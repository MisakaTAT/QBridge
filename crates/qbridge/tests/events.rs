mod common;

use common::*;
use qbridge::store::{Conversation, IdentityKind};
use qbridge_ob11::{Event, EventKind, MessageFormat};
use qbridge_qq::Envelope;
use serde_json::{Value, json};

fn event_value(event: &Event) -> Value {
    event.to_value(MessageFormat::Array)
}

#[tokio::test]
async fn translates_c2c_messages_and_caches_content() {
    let service = service().await;
    let store = memory_store();
    service.fake.respond(200, json!({}));
    let bridge = service.bridge(store.clone());
    let envelope = Envelope::from_json(&json!({
        "id": "EVENT",
        "t": "C2C_MESSAGE_CREATE",
        "d": {
            "id": "OFFICIAL",
            "content": "hello",
            "timestamp": "2099-01-01T00:00:00Z",
            "author": {"user_openid": "USER", "username": "Alice"},
            "message_scene": {"ext": ["msg_idx=REF"]},
            "attachments": [{
                "content_type": "image/png",
                "url": "https://example.com/a.png",
                "filename": "a.png"
            }]
        }
    }))
    .unwrap();
    let event = bridge.translate(envelope).await.unwrap();
    let value = event_value(&event);
    assert_eq!(value["post_type"], "message");
    assert_eq!(value["message_type"], "private");
    assert_eq!(value["sub_type"], "friend");
    assert!(value["message_id"].as_i64().unwrap() > 0);
    assert_eq!(value["message"][0]["type"], "text");
    assert_eq!(value["message"][0]["data"]["text"], "hello");
    assert_eq!(value["message"][1]["type"], "image");
    assert_eq!(
        value["message"][1]["data"]["url"],
        "https://example.com/a.png"
    );
    assert_eq!(value["sender"]["nickname"], "Alice");
    assert_eq!(value["qq"]["message_id"], "OFFICIAL");
    assert_eq!(value["qq"]["event_type"], "C2C_MESSAGE_CREATE");
    assert_eq!(value["qq"]["author"]["user_openid"], "USER");

    let id = value["message_id"].as_i64().unwrap() as i32;
    let cached = store.message(id, 0).await.unwrap().unwrap();
    assert_eq!(cached["message_id"], id);
    assert_eq!(cached["message"][0]["data"]["text"], "hello");
    let user = store.identity(IdentityKind::User, "USER").await.unwrap();
    assert_eq!(value["user_id"], user);
    let quote = store
        .quote(Conversation::Private(user), id, 0)
        .await
        .unwrap();
    assert_eq!(quote.reference.as_deref(), Some("REF"));
}

#[tokio::test]
async fn translates_group_messages_with_member_identity_and_reference() {
    let service = service().await;
    let store = memory_store();
    let bridge = service.bridge(store.clone());
    let envelope = Envelope::from_json(&json!({
        "t": "GROUP_AT_MESSAGE_CREATE",
        "d": {
            "id": "OFFICIAL",
            "content": "hello",
            "timestamp": "2099-01-01T00:00:00Z",
            "group_openid": "GROUP",
            "author": {"member_openid": "MEMBER", "username": "Bob", "member_role": "admin"},
            "message_scene": {"ext": ["msg_idx=REF-G"]}
        }
    }))
    .unwrap();
    let event = bridge.translate(envelope).await.unwrap();
    let value = event_value(&event);
    assert_eq!(value["message_type"], "group");
    assert_eq!(value["sub_type"], "normal");
    assert_eq!(value["anonymous"], Value::Null);
    assert_eq!(value["sender"]["role"], "admin");
    assert_eq!(value["qq"]["event_type"], "GROUP_AT_MESSAGE_CREATE");
    let group = store.identity(IdentityKind::Group, "GROUP").await.unwrap();
    let member = store
        .identity(IdentityKind::Member, "MEMBER")
        .await
        .unwrap();
    assert_eq!(value["group_id"], group);
    assert_eq!(value["user_id"], member);
    let id = value["message_id"].as_i64().unwrap() as i32;
    let quote = store
        .quote(Conversation::Group(group), id, 0)
        .await
        .unwrap();
    assert_eq!(quote.reference.as_deref(), Some("REF-G"));
}

#[tokio::test]
async fn duplicate_official_messages_are_dropped() {
    let service = service().await;
    let store = memory_store();
    let bridge = service.bridge(store);
    let json = json!({
        "t": "C2C_MESSAGE_CREATE",
        "d": {
            "id": "OFFICIAL",
            "content": "hello",
            "timestamp": "2099-01-01T00:00:00Z",
            "author": {"user_openid": "USER"}
        }
    });
    assert!(
        bridge
            .translate(Envelope::from_json(&json).unwrap())
            .await
            .is_some()
    );
    assert!(
        bridge
            .translate(Envelope::from_json(&json).unwrap())
            .await
            .is_none()
    );
}

#[tokio::test]
async fn translates_group_and_friend_notices() {
    let service = service().await;
    let store = memory_store();
    let bridge = service.bridge(store.clone());
    let group_event = Envelope::from_json(&json!({
        "id": "E1",
        "t": "GROUP_ADD_ROBOT",
        "d": {"group_openid": "GROUP", "op_member_openid": "MEMBER", "timestamp": "2099-01-01T00:00:00Z"}
    }))
    .unwrap();
    let event = bridge.translate(group_event).await.unwrap();
    let value = event_value(&event);
    assert_eq!(value["post_type"], "notice");
    assert_eq!(value["notice_type"], "group_increase");
    assert_eq!(value["sub_type"], "invite");
    assert_eq!(value["qq"]["event_type"], "GROUP_ADD_ROBOT");
    assert_eq!(value["qq"]["event_id"], "E1");
    let group = store.identity(IdentityKind::Group, "GROUP").await.unwrap();
    assert_eq!(value["group_id"], group);

    let friend_event = Envelope::from_json(&json!({
        "t": "FRIEND_ADD",
        "d": {"openid": "USER", "timestamp": "2099-01-01T00:00:00Z"}
    }))
    .unwrap();
    let event = bridge.translate(friend_event).await.unwrap();
    let value = event_value(&event);
    assert_eq!(value["notice_type"], "friend_add");
    assert!(value["group_id"].is_null());
}

#[tokio::test]
async fn translates_interaction_notices() {
    let service = service().await;
    let store = memory_store();
    let bridge = service.bridge(store);
    let envelope = Envelope::from_json(&json!({
        "t": "INTERACTION_CREATE",
        "d": {
            "id": "I1",
            "type": 11,
            "chat_type": 1,
            "group_openid": "GROUP",
            "group_member_openid": "MEMBER",
            "timestamp": "2099-01-01T00:00:00Z"
        }
    }))
    .unwrap();
    let event = bridge.translate(envelope).await.unwrap();
    let value = event_value(&event);
    assert_eq!(value["notice_type"], "interaction");
    assert_eq!(value["sub_type"], "button");
    assert_eq!(value["interaction_id"], "I1");
    assert_eq!(value["qq"]["event_type"], "INTERACTION_CREATE");
}

#[tokio::test]
async fn translates_remaining_group_and_c2c_notices() {
    let service = service().await;
    let store = memory_store();
    let bridge = service.bridge(store);
    let cases = [
        (
            "GROUP_DEL_ROBOT",
            json!({"group_openid": "GROUP", "op_member_openid": "MEMBER", "timestamp": "2099-01-01T00:00:00Z"}),
            "group_decrease",
        ),
        (
            "GROUP_MSG_RECEIVE",
            json!({"group_openid": "GROUP", "op_member_openid": "MEMBER", "timestamp": "2099-01-01T00:00:00Z"}),
            "group_receive",
        ),
        (
            "GROUP_MSG_REJECT",
            json!({"group_openid": "GROUP", "op_member_openid": "MEMBER", "timestamp": "2099-01-01T00:00:00Z"}),
            "group_reject",
        ),
        (
            "C2C_MSG_RECEIVE",
            json!({"openid": "USER", "timestamp": "2099-01-01T00:00:00Z"}),
            "c2c_msg_receive",
        ),
        (
            "C2C_MSG_REJECT",
            json!({"openid": "USER", "timestamp": "2099-01-01T00:00:00Z"}),
            "c2c_msg_reject",
        ),
        (
            "FRIEND_DEL",
            json!({"openid": "USER", "timestamp": "2099-01-01T00:00:00Z"}),
            "friend_decrease",
        ),
    ];
    for (event_type, data, expected) in cases {
        let envelope = Envelope::from_json(&json!({"t": event_type, "d": data})).unwrap();
        let event = bridge.translate(envelope).await.unwrap();
        let value = event_value(&event);
        assert_eq!(value["notice_type"], expected, "{event_type}");
        assert_eq!(value["qq"]["event_type"], event_type);
    }
}

#[tokio::test]
async fn translates_group_member_and_join_request_events() {
    let service = service().await;
    let store = memory_store();
    let bridge = service.bridge(store.clone());
    let add = Envelope::from_json(&json!({
        "t": "GROUP_MEMBER_ADD",
        "d": {
            "group_openid": "GROUP",
            "member_openid": "MEMBER",
            "timestamp": "2099-01-01T00:00:00Z"
        }
    }))
    .unwrap();
    let value = event_value(&bridge.translate(add).await.unwrap());
    assert_eq!(value["notice_type"], "group_increase");
    assert_eq!(value["sub_type"], "approve");
    assert_eq!(value["qq"]["event_type"], "GROUP_MEMBER_ADD");
    let group = store.identity(IdentityKind::Group, "GROUP").await.unwrap();
    let member = store
        .identity(IdentityKind::Member, "MEMBER")
        .await
        .unwrap();
    assert_eq!(value["group_id"], group);
    assert_eq!(value["user_id"], member);

    let remove = Envelope::from_json(&json!({
        "t": "GROUP_MEMBER_REMOVE",
        "d": {
            "group_openid": "GROUP",
            "member_openid": "MEMBER",
            "timestamp": 1795140226
        }
    }))
    .unwrap();
    let value = event_value(&bridge.translate(remove).await.unwrap());
    assert_eq!(value["notice_type"], "group_decrease");
    assert_eq!(value["sub_type"], "leave");
    assert_eq!(value["time"], 1795140226);
    assert!(value["operator_id"].is_null());

    let join = Envelope::from_json(&json!({
        "t": "GROUP_JOIN_REQUEST",
        "d": {
            "group_openid": "GROUP",
            "join_request_id": "JOIN",
            "member_openid": "APPLICANT",
            "apply_at": "2099-01-01T00:00:00Z",
            "risk_tips": "注意安全"
        }
    }))
    .unwrap();
    let value = event_value(&bridge.translate(join).await.unwrap());
    assert_eq!(value["post_type"], "request");
    assert_eq!(value["request_type"], "group");
    assert_eq!(value["sub_type"], "add");
    assert_eq!(value["comment"], "注意安全");
    let applicant = store
        .identity(IdentityKind::Member, "APPLICANT")
        .await
        .unwrap();
    assert_eq!(value["user_id"], applicant);
    let flag = value["flag"].as_str().unwrap();
    assert!(flag.starts_with(&format!("qq_group:{group}:{applicant}:JOIN")));
}

#[tokio::test]
async fn maps_optional_group_operator_when_qq_includes_it() {
    let service = service().await;
    let store = memory_store();
    let bridge = service.bridge(store);
    let remove = Envelope::from_json(&json!({
        "t": "GROUP_MEMBER_REMOVE",
        "d": {
            "group_openid": "GROUP",
            "member_openid": "MEMBER",
            "op_member_openid": "OPERATOR",
            "timestamp": 1795140226
        }
    }))
    .unwrap();
    let value = event_value(&bridge.translate(remove).await.unwrap());
    assert_eq!(value["notice_type"], "group_decrease");
    assert_eq!(value["sub_type"], "kick");
    assert!(!value["operator_id"].is_null());
}

#[tokio::test]
async fn maps_audio_and_video_attachments() {
    let service = service().await;
    let store = memory_store();
    let bridge = service.bridge(store);
    let envelope = Envelope::from_json(&json!({
        "t": "C2C_MESSAGE_CREATE",
        "d": {
            "id": "OFFICIAL",
            "content": "",
            "timestamp": "2099-01-01T00:00:00Z",
            "author": {"user_openid": "USER"},
            "attachments": [
                {"content_type": "audio/mpeg", "url": "https://example.com/a.mp3"},
                {"content_type": "video/mp4", "url": "https://example.com/a.mp4"}
            ]
        }
    }))
    .unwrap();
    let event = bridge.translate(envelope).await.unwrap();
    let value = event_value(&event);
    assert_eq!(value["message"][0]["type"], "record");
    assert_eq!(value["message"][1]["type"], "video");
}

#[tokio::test]
async fn maps_unknown_attachments_to_a_custom_segment() {
    let service = service().await;
    let store = memory_store();
    let bridge = service.bridge(store);
    let envelope = Envelope::from_json(&json!({
        "t": "C2C_MESSAGE_CREATE",
        "d": {
            "id": "OFFICIAL",
            "content": "",
            "timestamp": "2099-01-01T00:00:00Z",
            "author": {"user_openid": "USER"},
            "attachments": [{
                "content_type": "application/zip",
                "url": "https://example.com/a.zip",
                "filename": "a.zip"
            }]
        }
    }))
    .unwrap();
    let event = bridge.translate(envelope).await.unwrap();
    let value = event_value(&event);
    assert_eq!(value["message"][0]["type"], "qq_attachment");
    assert_eq!(
        value["message"][0]["data"]["url"],
        "https://example.com/a.zip"
    );
}

#[tokio::test]
async fn ready_updates_the_bot_nickname_and_unknown_events_are_ignored() {
    let service = service().await;
    let store = memory_store();
    let bridge = service.bridge(store);
    let ready = Envelope::from_json(&json!({
        "t": "READY",
        "d": {"session_id": "S", "user": {"username": "Bot Name"}}
    }))
    .unwrap();
    assert!(bridge.translate(ready).await.is_none());
    assert_eq!(bridge.state().nickname().await, "Bot Name");

    let unknown = Envelope::from_json(&json!({"t": "SOMETHING_NEW", "d": {}})).unwrap();
    assert!(bridge.translate(unknown).await.is_none());
}

#[tokio::test]
async fn malformed_events_are_skipped() {
    let service = service().await;
    let store = memory_store();
    let bridge = service.bridge(store.clone());
    let malformed = Envelope::from_json(&json!({
        "t": "C2C_MESSAGE_CREATE",
        "d": {
            "id": "OFFICIAL",
            "content": "hello",
            "timestamp": "not-a-timestamp",
            "author": {"user_openid": "USER"}
        }
    }))
    .unwrap();
    assert!(bridge.translate(malformed).await.is_none());
    assert_eq!(store.clean_expired(0).await.unwrap(), 0);
}

#[tokio::test]
async fn group_message_scene_reference_is_stored_for_quotes() {
    let service = service().await;
    let store = memory_store();
    let bridge = service.bridge(store.clone());
    let envelope = Envelope::from_json(&json!({
        "t": "GROUP_MESSAGE_CREATE",
        "d": {
            "id": "OFFICIAL",
            "content": "hello",
            "timestamp": "2099-01-01T00:00:00Z",
            "group_openid": "GROUP",
            "author": {"member_openid": "MEMBER"},
            "message_scene": {"ext": ["msg_idx=REF"]}
        }
    }))
    .unwrap();
    let event = bridge.translate(envelope).await.unwrap();
    let EventKind::Message(message) = &event.kind else {
        panic!("unexpected event")
    };
    let quote = store
        .quote(
            Conversation::Group(message.scene_group_id()),
            message.message_id,
            0,
        )
        .await
        .unwrap();
    assert_eq!(quote.reference.as_deref(), Some("REF"));
}

trait SceneGroup {
    fn scene_group_id(&self) -> i64;
}

impl SceneGroup for qbridge_ob11::event::MessageEvent {
    fn scene_group_id(&self) -> i64 {
        match self.scene {
            qbridge_ob11::event::MessageScene::Group { group_id, .. } => group_id,
            _ => panic!("not a group message"),
        }
    }
}
