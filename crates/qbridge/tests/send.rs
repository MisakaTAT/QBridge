mod common;

use common::*;
use qbridge::{
    bridge::Bridge,
    error::{ActionError, UnsupportedKind},
    store::{Conversation, IdentityKind, Store},
};
use qbridge_ob11::{Action, Call, action::QuickOperation};
use serde_json::{Map, Value, json};

async fn run(bridge: &Bridge, action: &str, params: Value) -> Result<Value, ActionError> {
    let params: Map<String, Value> = params.as_object().unwrap().clone();
    let call = Call::parse(action, params).unwrap();
    bridge.execute(call.action).await
}

async fn cached_inbound(
    store: &Store,
    conversation: Conversation,
    official: &str,
    reference: Option<&str>,
    time: i64,
) -> i32 {
    store
        .save_inbound(
            conversation,
            official,
            time,
            time + 1000,
            &message_event("group", conversation.target(), "hi", time),
            reference,
        )
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn sends_group_text_with_passive_context() {
    let service = service().await;
    let store = memory_store();
    let group = store.identity(IdentityKind::Group, "GROUP").await.unwrap();
    let conversation = Conversation::Group(group);
    cached_inbound(&store, conversation, "IN", None, now() - 5).await;
    service.fake.respond(200, json!({"id": "OUT"}));
    let bridge = service.bridge(store.clone());

    let result = run(
        &bridge,
        "send_group_msg",
        json!({"group_id": group, "message": "hello"}),
    )
    .await
    .unwrap();
    assert_eq!(result["message_id"], 2);

    let requests = service.fake.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/v2/groups/GROUP/messages");
    assert_eq!(requests[0].body["msg_type"], 0);
    assert_eq!(requests[0].body["content"], "hello");
    assert_eq!(requests[0].body["msg_id"], "IN");
    assert_eq!(requests[0].body["msg_seq"], 1);

    let cached = store.message(2, now()).await.unwrap().unwrap();
    assert_eq!(cached["message_id"], 2);
    assert_eq!(cached["qq"]["message_id"], "OUT");
}

#[tokio::test]
async fn active_sends_omit_the_passive_context() {
    let service = service().await;
    let store = memory_store();
    let group = store.identity(IdentityKind::Group, "GROUP").await.unwrap();
    service.fake.respond(200, json!({"id": "OUT"}));
    let bridge = service.bridge(store.clone());

    run(
        &bridge,
        "send_group_msg",
        json!({"group_id": group, "message": "hello", "active": true}),
    )
    .await
    .unwrap();
    let request = service.fake.requests().remove(0);
    assert!(request.body.get("msg_id").is_none());
    assert!(request.body.get("msg_seq").is_none());
}

#[tokio::test]
async fn uploads_media_before_sending() {
    let service = service().await;
    let store = memory_store();
    let group = store.identity(IdentityKind::Group, "GROUP").await.unwrap();
    let conversation = Conversation::Group(group);
    cached_inbound(&store, conversation, "IN", None, now() - 5).await;
    service.fake.respond(200, json!({"file_info": "FILE"}));
    service.fake.respond(200, json!({"id": "OUT"}));
    let bridge = service.bridge(store);

    run(
        &bridge,
        "send_group_msg",
        json!({
            "group_id": group,
            "message": [
                {"type": "text", "data": {"text": "caption"}},
                {"type": "image", "data": {"file": "https://example.com/a.png"}}
            ]
        }),
    )
    .await
    .unwrap();

    let requests = service.fake.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].path, "/v2/groups/GROUP/files");
    assert_eq!(requests[0].body["file_type"], 1);
    assert_eq!(requests[0].body["srv_send_msg"], false);
    assert_eq!(requests[0].body["url"], "https://example.com/a.png");
    assert_eq!(requests[1].body["msg_type"], 7);
    assert_eq!(requests[1].body["content"], "caption");
    assert_eq!(requests[1].body["media"]["file_info"], "FILE");
}

#[tokio::test]
async fn converts_group_mentions_to_openid_placeholders() {
    let service = service().await;
    let store = memory_store();
    let group = store.identity(IdentityKind::Group, "GROUP").await.unwrap();
    let member = store
        .identity(IdentityKind::Member, "MEMBER")
        .await
        .unwrap();
    let conversation = Conversation::Group(group);
    cached_inbound(&store, conversation, "IN", None, now() - 5).await;
    service.fake.respond(200, json!({"id": "OUT"}));
    let bridge = service.bridge(store);

    run(
        &bridge,
        "send_group_msg",
        json!({
            "group_id": group,
            "message": [
                {"type": "at", "data": {"qq": member}},
                {"type": "text", "data": {"text": " hello"}}
            ]
        }),
    )
    .await
    .unwrap();
    let request = service.fake.requests().remove(0);
    assert_eq!(request.body["content"], "<@!MEMBER> hello");
}

#[tokio::test]
async fn reply_to_inbound_uses_its_context_and_reference() {
    let service = service().await;
    let store = memory_store();
    let group = store.identity(IdentityKind::Group, "GROUP").await.unwrap();
    let conversation = Conversation::Group(group);
    let inbound = cached_inbound(&store, conversation, "IN", Some("REF-IN"), now() - 5).await;
    service.fake.respond(200, json!({"id": "OUT"}));
    let bridge = service.bridge(store);

    run(
        &bridge,
        "send_group_msg",
        json!({
            "group_id": group,
            "message": [
                {"type": "reply", "data": {"id": inbound}},
                {"type": "text", "data": {"text": "answer"}}
            ]
        }),
    )
    .await
    .unwrap();
    let request = service.fake.requests().remove(0);
    assert_eq!(request.body["msg_id"], "IN");
    assert_eq!(request.body["msg_seq"], 1);
    assert_eq!(request.body["message_reference"]["message_id"], "REF-IN");
}

#[tokio::test]
async fn reply_to_bot_message_uses_latest_inbound_context() {
    let service = service().await;
    let store = memory_store();
    let group = store.identity(IdentityKind::Group, "GROUP").await.unwrap();
    let conversation = Conversation::Group(group);
    cached_inbound(&store, conversation, "IN", None, now() - 10).await;
    let outbound = store
        .save_outbound(
            conversation,
            "OUT-OLD",
            now() - 5,
            now() + 1000,
            &message_event("group", group, "bot", now() - 5),
            Some("REF-OUT"),
        )
        .await
        .unwrap();
    service.fake.respond(200, json!({"id": "OUT"}));
    let bridge = service.bridge(store);

    run(
        &bridge,
        "send_group_msg",
        json!({
            "group_id": group,
            "message": [
                {"type": "reply", "data": {"id": outbound}},
                {"type": "text", "data": {"text": "answer"}}
            ]
        }),
    )
    .await
    .unwrap();
    let request = service.fake.requests().remove(0);
    assert_eq!(request.body["msg_id"], "IN");
    assert_eq!(request.body["msg_seq"], 1);
    assert_eq!(request.body["message_reference"]["message_id"], "REF-OUT");
}

#[tokio::test]
async fn unified_send_msg_selects_private_targets() {
    let service = service().await;
    let store = memory_store();
    let user = store.identity(IdentityKind::User, "USER").await.unwrap();
    service.fake.respond(200, json!({"id": "OUT"}));
    let bridge = service.bridge(store);
    run(
        &bridge,
        "send_msg",
        json!({"message_type": "private", "user_id": user, "message": "hello", "active": true}),
    )
    .await
    .unwrap();
    let request = service.fake.requests().remove(0);
    assert_eq!(request.path, "/v2/users/USER/messages");
    assert_eq!(request.body["content"], "hello");
}

#[tokio::test]
async fn uploads_base64_and_local_file_media() {
    let service = service().await;
    let store = memory_store();
    let group = store.identity(IdentityKind::Group, "GROUP").await.unwrap();
    let bridge = service.bridge(store);
    service.fake.respond(200, json!({"file_info": "B64"}));
    service.fake.respond(200, json!({"id": "OUT1"}));
    run(
        &bridge,
        "send_group_msg",
        json!({
            "group_id": group,
            "active": true,
            "message": {"type": "image", "data": {"file": "base64://QUJD"}}
        }),
    )
    .await
    .unwrap();

    let path = std::env::temp_dir().join(format!("qbridge-media-{}.png", std::process::id()));
    std::fs::write(&path, b"local-bytes").unwrap();
    service.fake.respond(200, json!({"file_info": "FILE"}));
    service.fake.respond(200, json!({"id": "OUT2"}));
    run(
        &bridge,
        "send_group_msg",
        json!({
            "group_id": group,
            "active": true,
            "message": {"type": "image", "data": {"file": url::Url::from_file_path(&path).unwrap().to_string()}}
        }),
    )
    .await
    .unwrap();
    let _ = std::fs::remove_file(&path);
    let requests = service.fake.requests();
    assert_eq!(requests[0].body["file_data"], "QUJD");
    assert_eq!(requests[0].body["file_name"], "image.png");
    assert_eq!(requests[2].body["file_data"], "bG9jYWwtYnl0ZXM=");
    assert_eq!(
        requests[2].body["file_name"],
        path.file_name().unwrap().to_str().unwrap()
    );
}

#[tokio::test]
async fn uploads_record_and_video_file_types() {
    let service = service().await;
    let store = memory_store();
    let group = store.identity(IdentityKind::Group, "GROUP").await.unwrap();
    let bridge = service.bridge(store);
    for (file_type, segment) in [
        (
            3,
            json!({"type": "record", "data": {"file": "base64://QUJD"}}),
        ),
        (
            2,
            json!({"type": "video", "data": {"file": "base64://QUJD"}}),
        ),
    ] {
        service
            .fake
            .respond(200, json!({"file_info": format!("F{file_type}")}));
        service
            .fake
            .respond(200, json!({"id": format!("OUT{file_type}")}));
        run(
            &bridge,
            "send_group_msg",
            json!({"group_id": group, "active": true, "message": segment}),
        )
        .await
        .unwrap();
        let requests = service.fake.take_requests();
        assert_eq!(requests[0].body["file_type"], file_type);
        assert_eq!(
            requests[1].body["media"]["file_info"],
            format!("F{file_type}")
        );
    }
}

#[tokio::test]
async fn uploads_large_local_video_in_chunks() {
    let service = service().await;
    let store = memory_store();
    let group = store.identity(IdentityKind::Group, "GROUP").await.unwrap();
    let bridge = service.bridge(store);
    let path = std::env::temp_dir().join(format!("qbridge-chunk-{}.mp4", std::process::id()));
    let size = 5 * 1024 * 1024 + 1;
    std::fs::write(&path, vec![0u8; size]).unwrap();
    let uri = url::Url::from_file_path(&path).unwrap().to_string();
    service.fake.respond(
        200,
        json!({
            "upload_id": "UPLOAD",
            "block_size": size.to_string(),
            "parts": [{
                "index": 0,
                "presigned_url": format!("{}/part/0", service.base),
                "block_size": size.to_string()
            }]
        }),
    );
    service.fake.respond(200, json!({}));
    service.fake.respond(200, json!({}));
    service.fake.respond(200, json!({"file_info": "FILE"}));
    service.fake.respond(200, json!({"id": "OUT"}));
    run(
        &bridge,
        "send_group_msg",
        json!({
            "group_id": group,
            "active": true,
            "message": {"type": "video", "data": {"file": uri}}
        }),
    )
    .await
    .unwrap();
    let requests = service.fake.requests();
    assert_eq!(requests[0].path, "/v2/groups/GROUP/upload_prepare");
    assert_eq!(requests[0].body["file_type"], 2);
    assert_eq!(requests[0].body["file_size"], size.to_string());
    assert_eq!(requests[1].method, "PUT");
    assert_eq!(requests[2].path, "/v2/groups/GROUP/upload_part_finish");
    assert_eq!(requests[3].path, "/v2/groups/GROUP/files");
    assert_eq!(requests[3].body["upload_id"], "UPLOAD");
    assert_eq!(requests[4].path, "/v2/groups/GROUP/messages");
    assert_eq!(requests[4].body["msg_type"], 7);
    assert_eq!(requests[4].body["media"]["file_info"], "FILE");
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn sends_markdown_templates_and_custom_keyboards() {
    let service = service().await;
    let store = memory_store();
    let group = store.identity(IdentityKind::Group, "GROUP").await.unwrap();
    let conversation = Conversation::Group(group);
    cached_inbound(&store, conversation, "IN", None, now() - 5).await;
    service.fake.respond(200, json!({"id": "OUT"}));
    let bridge = service.bridge(store);
    run(
        &bridge,
        "send_group_msg",
        json!({
            "group_id": group,
            "message": [
                {"type": "markdown", "data": {
                    "custom_template_id": "T1",
                    "params": [{"key": "k", "values": ["v"]}]
                }},
                {"type": "keyboard", "data": {"content": {"rows": [{"buttons": [{
                    "render_data": {"label": "go", "style": 1},
                    "action": {"type": 1, "data": "payload"}
                }]}]}}}
            ]
        }),
    )
    .await
    .unwrap();
    let request = service.fake.requests().remove(0);
    assert_eq!(request.body["msg_type"], 2);
    assert_eq!(request.body["markdown"]["custom_template_id"], "T1");
    assert_eq!(request.body["markdown"]["params"][0]["key"], "k");
    assert_eq!(
        request.body["keyboard"]["content"]["rows"][0]["buttons"][0]["render_data"]["label"],
        "go"
    );
}

#[tokio::test]
async fn sends_markdown_with_a_keyboard() {
    let service = service().await;
    let store = memory_store();
    let group = store.identity(IdentityKind::Group, "GROUP").await.unwrap();
    let conversation = Conversation::Group(group);
    cached_inbound(&store, conversation, "IN", None, now() - 5).await;
    service.fake.respond(200, json!({"id": "OUT"}));
    let bridge = service.bridge(store);
    run(
        &bridge,
        "send_group_msg",
        json!({
            "group_id": group,
            "message": [
                {"type": "markdown", "data": {"content": "# hi"}},
                {"type": "keyboard", "data": {"id": "TEMPLATE"}}
            ]
        }),
    )
    .await
    .unwrap();
    let request = service.fake.requests().remove(0);
    assert_eq!(request.body["msg_type"], 2);
    assert_eq!(request.body["markdown"]["content"], "# hi");
    assert_eq!(request.body["keyboard"]["id"], "TEMPLATE");
}

#[tokio::test]
async fn quick_operation_successfully_sends_a_reply() {
    let service = service().await;
    let store = memory_store();
    let group = store.identity(IdentityKind::Group, "GROUP").await.unwrap();
    let conversation = Conversation::Group(group);
    cached_inbound(&store, conversation, "IN", None, now() - 5).await;
    service.fake.respond(200, json!({"id": "OUT"}));
    let bridge = service.bridge(store);
    let operation = QuickOperation {
        context: json!({
            "self_id": 10_000,
            "post_type": "message",
            "message_type": "group",
            "group_id": group,
            "user_id": 42,
            "message_id": 1
        })
        .as_object()
        .unwrap()
        .clone(),
        operation: json!({"reply": "pong", "at_sender": false})
            .as_object()
            .unwrap()
            .clone(),
    };
    let result = bridge
        .execute(Action::HandleQuickOperation(operation))
        .await
        .unwrap();
    assert_eq!(result, Value::Null);
    let request = service.fake.requests().remove(0);
    assert_eq!(request.body["content"], "pong");
    assert_eq!(request.body["msg_id"], "IN");
}

#[tokio::test]
async fn invalid_combinations_fail_before_network_access() {
    let service = service().await;
    let store = memory_store();
    let group = store.identity(IdentityKind::Group, "GROUP").await.unwrap();
    let bridge = service.bridge(store);
    let error = run(
        &bridge,
        "send_group_msg",
        json!({
            "group_id": group,
            "message": [
                {"type": "image", "data": {"file": "https://example.com/a.png"}},
                {"type": "image", "data": {"file": "https://example.com/b.png"}}
            ]
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(
        error,
        ActionError::Unsupported(UnsupportedKind::MessageCombination)
    );
    assert!(service.fake.requests().is_empty());
}

#[tokio::test]
async fn missing_qq_message_id_is_reported_as_an_unknown_outcome() {
    let service = service().await;
    let store = memory_store();
    let group = store.identity(IdentityKind::Group, "GROUP").await.unwrap();
    let conversation = Conversation::Group(group);
    cached_inbound(&store, conversation, "IN", None, now() - 5).await;
    service.fake.respond(200, json!({}));
    let bridge = service.bridge(store);
    let error = run(
        &bridge,
        "send_group_msg",
        json!({"group_id": group, "message": "hello"}),
    )
    .await
    .unwrap_err();
    assert_eq!(error, ActionError::SendOutcomeUnknown);
}

#[tokio::test]
async fn quick_operations_are_fully_validated_before_execution() {
    let service = service().await;
    let store = memory_store();
    let group = store.identity(IdentityKind::Group, "GROUP").await.unwrap();
    let bridge = service.bridge(store);
    let operation = QuickOperation {
        context: json!({
            "self_id": 10_000,
            "post_type": "message",
            "message_type": "group",
            "group_id": group,
            "user_id": 42,
            "message_id": 1,
            "anonymous": {"id": 1, "name": "anon", "flag": "flag"}
        })
        .as_object()
        .unwrap()
        .clone(),
        operation: json!({"reply": "hello", "ban": true})
            .as_object()
            .unwrap()
            .clone(),
    };
    let error = bridge
        .execute(Action::HandleQuickOperation(operation))
        .await
        .unwrap_err();
    assert_eq!(error, ActionError::Unsupported(UnsupportedKind::Action));
    assert!(service.fake.requests().is_empty());
}

#[tokio::test]
async fn pure_node_message_is_sent_as_markdown() {
    let service = service().await;
    let store = memory_store();
    let group = store.identity(IdentityKind::Group, "GROUP").await.unwrap();
    let conversation = Conversation::Group(group);
    cached_inbound(&store, conversation, "IN", None, now() - 5).await;
    service.fake.respond(200, json!({"id": "OUT"}));
    let bridge = service.bridge(store);

    run(
        &bridge,
        "send_group_msg",
        json!({
            "group_id": group,
            "message": [{
                "type": "node",
                "data": {"user_id": "1", "nickname": "A", "content": [{"type": "text", "data": {"text": "hi"}}]}
            }],
            "source": "History"
        }),
    )
    .await
    .unwrap();
    let request = service.fake.requests().remove(0);
    assert_eq!(request.body["msg_type"], 2);
    let content = request.body["markdown"]["content"].as_str().unwrap();
    assert!(content.starts_with("# History\n"));
    assert!(content.contains("**A"));
}

#[tokio::test]
async fn sends_card_input_notify_and_wakeup_messages() {
    let service = service().await;
    let store = memory_store();
    let group = store.identity(IdentityKind::Group, "GROUP").await.unwrap();
    let user = store.identity(IdentityKind::User, "USER").await.unwrap();
    let bridge = service.bridge(store);
    service.fake.respond(200, json!({"id": "C1"}));
    service.fake.respond(200, json!({}));
    service.fake.respond(200, json!({"id": "W1"}));
    run(
        &bridge,
        "send_group_msg",
        json!({
            "group_id": group,
            "active": true,
            "message": "[CQ:card,title=T,desc=D,pic=https://example.com/p.png,url=https://example.com]"
        }),
    )
    .await
    .unwrap();
    let notify = run(
        &bridge,
        "send_private_msg",
        json!({
            "user_id": user,
            "active": true,
            "message": "[CQ:input_notify,type=1,second=30]"
        }),
    )
    .await
    .unwrap();
    assert_eq!(notify["message_id"], 0);
    run(
        &bridge,
        "send_private_msg",
        json!({
            "user_id": user,
            "message": format!("[CQ:wakeup,userid={user}]hi")
        }),
    )
    .await
    .unwrap();
    let requests = service.fake.requests();
    assert_eq!(requests[0].body["msg_type"], 8);
    assert_eq!(requests[0].body["card"]["type"], "tuwen");
    assert_eq!(requests[0].body["card"]["content"]["title"], "T");
    assert_eq!(requests[1].body["msg_type"], 6);
    assert_eq!(requests[1].body["input_notify"]["input_type"], 1);
    assert_eq!(requests[2].body["is_wakeup"], true);
    assert_eq!(requests[2].body["content"], "hi");
}

#[tokio::test]
async fn sends_a_file_segment_as_rich_media() {
    let service = service().await;
    let store = memory_store();
    let group = store.identity(IdentityKind::Group, "GROUP").await.unwrap();
    let bridge = service.bridge(store);
    service.fake.respond(200, json!({"file_info": "FILE"}));
    service.fake.respond(200, json!({"id": "F1"}));
    run(
        &bridge,
        "send_group_msg",
        json!({
            "group_id": group,
            "active": true,
            "message": "[CQ:file,file=https://example.com/a.pdf,name=a.pdf]"
        }),
    )
    .await
    .unwrap();
    let requests = service.fake.requests();
    assert_eq!(requests[0].path, "/v2/groups/GROUP/files");
    assert_eq!(requests[0].body["file_type"], 4);
    assert_eq!(requests[0].body["file_name"], "a.pdf");
    assert_eq!(requests[1].body["msg_type"], 7);
    assert_eq!(requests[1].body["media"]["file_info"], "FILE");
}

#[tokio::test]
async fn expands_avatar_and_group_info_content_codes() {
    let service = service().await;
    let store = memory_store();
    let group = store.identity(IdentityKind::Group, "GROUP").await.unwrap();
    let user = store.identity(IdentityKind::User, "USER").await.unwrap();
    let bridge = service.bridge(store);
    service
        .fake
        .respond(200, json!({"group_name": "Group", "group_member_num": 3}));
    service
        .fake
        .respond(200, json!({"group_name": "Group", "group_member_num": 3}));
    service.fake.respond(200, json!({"id": "G1"}));
    run(
        &bridge,
        "send_group_msg",
        json!({
            "group_id": group,
            "active": true,
            "message": format!("[CQ:avatar,user_id={user}] [CQ:group_info,field=group_name,group_id={group}] [CQ:group_info,field=member_count,group_id={group}]")
        }),
    )
    .await
    .unwrap();
    let requests = service.fake.requests();
    let content = requests[2].body["content"].as_str().unwrap();
    assert!(content.contains("/USER/640"));
    assert!(content.contains("Group 3"));
    assert_eq!(requests[0].path, "/v2/groups/GROUP/info");
}

#[tokio::test]
async fn sends_stream_start_mid_and_finish() {
    let service = service().await;
    let store = memory_store();
    let user = store.identity(IdentityKind::User, "USER").await.unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    store
        .save_inbound(
            Conversation::Private(user),
            "IN",
            now,
            now + 1000,
            &message_event("private", user, "hi", now),
            None,
        )
        .await
        .unwrap();
    let bridge = service.bridge(store);
    service.fake.respond(200, json!({"id": "S1"}));
    service.fake.respond(200, json!({"id": "S1"}));
    service.fake.respond(200, json!({"id": "S1"}));
    for (kind, text) in [("start", "he"), ("mid", "llo"), ("finish", "!")] {
        run(
            &bridge,
            "send_private_msg",
            json!({
                "user_id": user,
                "message": format!("[CQ:stream,type={kind}]{text}")
            }),
        )
        .await
        .unwrap();
    }
    let requests = service.fake.requests();
    assert_eq!(requests[0].path, "/v2/users/USER/stream_messages");
    assert_eq!(requests[0].body["input_mode"], "replace");
    assert_eq!(requests[0].body["input_state"], 1);
    assert_eq!(requests[0].body["index"], 0);
    assert_eq!(requests[0].body["content_raw"], "he");
    assert_eq!(requests[0].body["msg_id"], "IN");
    assert_eq!(requests[0].body["msg_seq"], 1);
    assert_eq!(requests[1].body["index"], 1);
    assert_eq!(requests[1].body["stream_msg_id"], "S1");
    assert_eq!(requests[1].body["content_raw"], "hello");
    assert_eq!(requests[2].body["index"], 2);
    assert_eq!(requests[2].body["input_state"], 10);
    assert_eq!(requests[2].body["stream_msg_id"], "S1");
    assert_eq!(requests[2].body["content_raw"], "hello!");
}

#[tokio::test]
async fn executes_set_group_and_strips_member_codes() {
    let service = service().await;
    let store = memory_store();
    let group = store.identity(IdentityKind::Group, "GROUP").await.unwrap();
    let user = store
        .identity(IdentityKind::Member, "MEMBER")
        .await
        .unwrap();
    let bridge = service.bridge(store);
    service.fake.respond(200, json!({}));
    service.fake.respond(200, json!({"id": "M1"}));
    service.fake.respond(200, json!({"id": "M2"}));
    let result = run(
        &bridge,
        "send_group_msg",
        json!({
            "group_id": group,
            "active": true,
            "message": format!("[CQ:set_group,action=ban,group_id={group},user_id={user},duration=60][CQ:member,type=add,group_id={group},user_id={user}]hello")
        }),
    )
    .await
    .unwrap();
    assert_eq!(result["message_id"], 1);
    let requests = service.fake.requests();
    assert_eq!(requests[0].path, "/v2/groups/GROUP/restrict_chat_setting");
    assert_eq!(requests[1].body["content"], "hello");

    service.fake.respond(200, json!({}));
    let pure = run(
        &bridge,
        "send_group_msg",
        json!({
            "group_id": group,
            "active": true,
            "message": format!("[CQ:set_group,action=ban,group_id={group},user_id={user},duration=60]")
        }),
    )
    .await
    .unwrap();
    assert_eq!(pure["message_id"], 0);
    assert_eq!(service.fake.requests().len(), 3);
}

#[tokio::test]
async fn sends_group_at_all_as_qqbot_tag() {
    let service = service().await;
    let store = memory_store();
    let group = store.identity(IdentityKind::Group, "GROUP").await.unwrap();
    let bridge = service.bridge(store);
    service.fake.respond(200, json!({"id": "ALL1"}));
    run(
        &bridge,
        "send_group_msg",
        json!({
            "group_id": group,
            "active": true,
            "message": "[CQ:at,qq=all] 公告"
        }),
    )
    .await
    .unwrap();
    let request = &service.fake.requests()[0];
    assert_eq!(request.body["content"], "<qqbot-at-everyone /> 公告");
}

#[tokio::test]
async fn uploads_local_images_inside_forward_nodes() {
    let service = service().await;
    let store = memory_store();
    let group = store.identity(IdentityKind::Group, "GROUP").await.unwrap();
    let bridge = service.bridge(store);
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/assets/test.png");
    service.fake.respond(
        200,
        json!({
            "upload_id": "U",
            "presigned_url": format!("{}/media", service.base),
            "expires_in": 3600
        }),
    );
    service.fake.respond(200, json!({}));
    service
        .fake
        .respond(200, json!({"url": "qqbot-media://image/v1/T"}));
    service.fake.respond(200, json!({"id": "FWD"}));
    run(
        &bridge,
        "send_group_forward_msg",
        json!({
            "group_id": group,
            "active": true,
            "messages": [{"type": "node", "data": {
                "user_id": "1", "nickname": "A",
                "content": [
                    {"type": "text", "data": {"text": "local"}},
                    {"type": "image", "data": {"file": format!("file://{path}")}}
                ]
            }}]
        }),
    )
    .await
    .unwrap();
    let requests = service.fake.requests();
    assert_eq!(requests[0].path, "/v2/media-images/prepare");
    assert_eq!(requests[2].path, "/v2/media-images/complete");
    let content = requests[3].body["markdown"]["content"].as_str().unwrap();
    assert!(content.contains("qqbot-media://image/v1/T"));
    assert!(content.contains("#64px #64px"));
    assert!(content.contains("local"));
}

#[tokio::test]
async fn stream_updates_cached_message_and_preserves_reference() {
    let service = service().await;
    let store = memory_store();
    let user = store.identity(IdentityKind::User, "USER").await.unwrap();
    let now = common::now();
    store
        .save_inbound(
            Conversation::Private(user),
            "IN",
            now,
            now + 1000,
            &message_event("private", user, "hi", now),
            None,
        )
        .await
        .unwrap();
    let bridge = service.bridge(store.clone());
    service.fake.respond(200, json!({"id": "S1"}));
    service.fake.respond(200, json!({"id": "S1"}));
    service.fake.respond(
        200,
        json!({"id": "S1", "ext_info": {"ref_idx": "STREAM-REF"}}),
    );
    let mut local_id = 0_i64;
    for (kind, text) in [("start", "he"), ("mid", "llo"), ("finish", "!")] {
        let result = run(
            &bridge,
            "send_private_msg",
            json!({
                "user_id": user,
                "message": format!("[CQ:stream,type={kind}]{text}")
            }),
        )
        .await
        .unwrap();
        local_id = result["message_id"].as_i64().unwrap();
    }
    let cached = run(&bridge, "get_msg", json!({"message_id": local_id}))
        .await
        .unwrap();
    assert_eq!(cached["message_id"], local_id);
    assert_eq!(cached["raw_message"], "hello!");
    assert_eq!(cached["message"][0]["data"]["text"], "hello!");
    let quote = store
        .quote(Conversation::Private(user), local_id as i32, now)
        .await
        .unwrap();
    assert_eq!(quote.reference.as_deref(), Some("STREAM-REF"));
}

#[tokio::test]
async fn expires_idle_stream_sessions_and_cleans_them() {
    let service = service().await;
    let store = memory_store();
    let user = store.identity(IdentityKind::User, "USER").await.unwrap();
    let now = common::now();
    store
        .save_inbound(
            Conversation::Private(user),
            "IN",
            now,
            now + 1000,
            &message_event("private", user, "hi", now),
            None,
        )
        .await
        .unwrap();
    let bridge = service.bridge_with_cache(
        store,
        qbridge::bridge::CachePolicy {
            stream_ttl_secs: 60,
            ..qbridge::bridge::CachePolicy::default()
        },
    );
    service.fake.respond(200, json!({"id": "S1"}));
    run(
        &bridge,
        "send_private_msg",
        json!({"user_id": user, "message": "[CQ:stream,type=start]he"}),
    )
    .await
    .unwrap();
    assert_eq!(bridge.clean_expired_streams(now + 30).await, 0);
    assert_eq!(bridge.clean_expired_streams(now + 61).await, 1);
    let error = run(
        &bridge,
        "send_private_msg",
        json!({"user_id": user, "message": "[CQ:stream,type=mid]llo"}),
    )
    .await
    .unwrap_err();
    assert!(matches!(error, ActionError::BadParam(_)));
}

#[tokio::test]
async fn serializes_concurrent_stream_chunks_for_the_same_user() {
    let service = service().await;
    let store = memory_store();
    let user = store.identity(IdentityKind::User, "USER").await.unwrap();
    let now = common::now();
    store
        .save_inbound(
            Conversation::Private(user),
            "IN",
            now,
            now + 1000,
            &message_event("private", user, "hi", now),
            None,
        )
        .await
        .unwrap();
    let bridge = std::sync::Arc::new(service.bridge(store));
    service.fake.respond(200, json!({"id": "S1"}));
    service.fake.respond(200, json!({"id": "S1"}));
    service.fake.respond(200, json!({"id": "S1"}));
    run(
        &bridge,
        "send_private_msg",
        json!({"user_id": user, "message": "[CQ:stream,type=start]A"}),
    )
    .await
    .unwrap();
    let (first, second) = tokio::join!(
        run(
            &bridge,
            "send_private_msg",
            json!({"user_id": user, "message": "[CQ:stream,type=mid]B"}),
        ),
        run(
            &bridge,
            "send_private_msg",
            json!({"user_id": user, "message": "[CQ:stream,type=mid]C"}),
        )
    );
    first.unwrap();
    second.unwrap();
    let requests = service.fake.requests();
    let mut indices = requests
        .iter()
        .map(|req| req.body["index"].as_u64().unwrap())
        .collect::<Vec<_>>();
    indices.sort();
    assert_eq!(indices, vec![0, 1, 2]);
    let final_text = requests[2].body["content_raw"].as_str().unwrap();
    assert_eq!(final_text.len(), 3);
    assert!(final_text.starts_with('A'));
    assert!(final_text.contains('B'));
    assert!(final_text.contains('C'));
}

#[tokio::test]
async fn applies_custom_cache_ttls_for_sent_inbound_and_profile() {
    let service = service().await;
    let store = memory_store();
    let group = store.identity(IdentityKind::Group, "GROUP").await.unwrap();
    let member = store
        .identity(IdentityKind::Member, "MEMBER")
        .await
        .unwrap();
    let bridge = service.bridge_with_cache(
        store.clone(),
        qbridge::bridge::CachePolicy {
            inbound_ttl_secs: 7200,
            sent_ttl_secs: 120,
            profile_ttl_secs: 90,
            stream_ttl_secs: 60,
        },
    );
    let now = common::now();

    service.fake.respond(200, json!({"id": "OUT-1"}));
    let sent = run(
        &bridge,
        "send_group_msg",
        json!({"group_id": group, "active": true, "message": "hello"}),
    )
    .await
    .unwrap();
    let sent_id = sent["message_id"].as_i64().unwrap() as i32;
    assert!(store.message(sent_id, now + 100).await.unwrap().is_some());
    assert!(store.message(sent_id, now + 130).await.unwrap().is_none());

    let envelope = qbridge_qq::Envelope::from_json(&json!({
        "t": "GROUP_AT_MESSAGE_CREATE",
        "d": {
            "id": "IN-1",
            "content": "ping",
            "group_openid": "GROUP",
            "timestamp": chrono::DateTime::from_timestamp(now, 0).unwrap().to_rfc3339(),
            "author": {"member_openid": "MEMBER"}
        }
    }))
    .unwrap();
    let event = bridge.translate(envelope).await.unwrap();
    let inbound_id = event.to_value(qbridge_ob11::MessageFormat::Array)["message_id"]
        .as_i64()
        .unwrap() as i32;
    assert!(
        store
            .message(inbound_id, now + 400)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .reserve_reply(Conversation::Group(group), Some(inbound_id), now + 400)
            .await
            .is_err()
    );
    assert!(
        store
            .message(inbound_id, now + 7205)
            .await
            .unwrap()
            .is_none()
    );

    service.fake.respond(
        200,
        json!({"member_openid": "MEMBER", "username": "Bob", "member_role": "member"}),
    );
    run(
        &bridge,
        "get_group_member_info",
        json!({"group_id": group, "user_id": member}),
    )
    .await
    .unwrap();
    assert!(store.profile(member, now + 60).await.unwrap().is_some());
    assert!(store.profile(member, now + 100).await.unwrap().is_none());
}

#[tokio::test]
async fn resolves_keyboard_permission_user_ids_to_openids() {
    let service = service().await;
    let store = memory_store();
    let group = store.identity(IdentityKind::Group, "GROUP").await.unwrap();
    let alice = store
        .identity(IdentityKind::Member, "ALICE_OPENID")
        .await
        .unwrap();
    let bob = store
        .identity(IdentityKind::User, "BOB_OPENID")
        .await
        .unwrap();
    service.fake.respond(200, json!({"id": "OUT-KB"}));
    let bridge = service.bridge(store);

    run(
        &bridge,
        "send_group_msg",
        json!({
            "group_id": group,
            "active": true,
            "message": [
                {"type": "markdown", "data": {"content": "choose"}},
                {
                    "type": "keyboard",
                    "data": {
                        "content": {
                            "rows": [{
                                "buttons": [{
                                    "id": "1",
                                    "render_data": {"label": "Go"},
                                    "action": {
                                        "type": 2,
                                        "data": "/go",
                                        "permission": {
                                            "type": 0,
                                            "specify_user_ids": [alice, bob.to_string(), "RAW_OPENID_ABC"]
                                        }
                                    }
                                }]
                            }]
                        }
                    }
                }
            ]
        }),
    )
    .await
    .unwrap();

    let request = service.fake.requests().remove(0);
    assert_eq!(
        request.body["keyboard"]["content"]["rows"][0]["buttons"][0]["action"]["permission"]["specify_user_ids"],
        json!(["ALICE_OPENID", "BOB_OPENID", "RAW_OPENID_ABC"])
    );

    let unknown = run(
        &bridge,
        "send_group_msg",
        json!({
            "group_id": group,
            "active": true,
            "message": [
                {"type": "markdown", "data": {"content": "choose"}},
                {
                    "type": "keyboard",
                    "data": {
                        "content": {
                            "rows": [{
                                "buttons": [{
                                    "render_data": {"label": "Go"},
                                    "action": {
                                        "type": 2,
                                        "data": "/go",
                                        "permission": {
                                            "type": 0,
                                            "specify_user_ids": [999_999]
                                        }
                                    }
                                }]
                            }]
                        }
                    }
                }
            ]
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(
        unknown,
        ActionError::NotFound(qbridge::error::NotFoundKind::UnknownTarget)
    );

    let invalid = run(
        &bridge,
        "send_group_msg",
        json!({
            "group_id": group,
            "active": true,
            "message": [
                {"type": "markdown", "data": {"content": "choose"}},
                {
                    "type": "keyboard",
                    "data": {
                        "content": {
                            "rows": [{
                                "buttons": [{
                                    "render_data": {"label": "Go"},
                                    "action": {
                                        "type": 2,
                                        "data": "/go",
                                        "permission": {
                                            "type": 0,
                                            "specify_user_ids": [true]
                                        }
                                    }
                                }]
                            }]
                        }
                    }
                }
            ]
        }),
    )
    .await
    .unwrap_err();
    assert!(matches!(invalid, ActionError::BadParam(_)));
}
