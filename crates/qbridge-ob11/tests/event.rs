use qbridge_ob11::event::{
    BotStatus, GroupDecreaseType, GroupIncreaseType, GroupRequestType, GroupSubType, LifecycleType,
    MessageEvent, MessageScene, MetaEvent, NoticeEvent, PrivateSubType, RequestEvent, RequestKind,
    Sender,
};
use qbridge_ob11::{Event, EventKind, Message, MessageFormat};
use serde_json::{Map, json};

fn event(kind: EventKind) -> Event {
    Event {
        time: 100,
        self_id: 42,
        kind,
    }
}

fn group_message() -> MessageEvent {
    MessageEvent::new(
        MessageScene::Group {
            group_id: 7,
            sub_type: GroupSubType::Normal,
            anonymous: None,
        },
        3,
        9,
        Message::text("[hi]"),
        Sender {
            user_id: 9,
            ..Sender::default()
        },
    )
}

#[test]
fn group_message_serializes_all_required_fields() {
    let value = event(EventKind::Message(group_message())).to_value(MessageFormat::Array);
    assert_eq!(
        value,
        json!({
            "time": 100, "self_id": 42, "post_type": "message",
            "message_type": "group", "sub_type": "normal", "group_id": 7, "anonymous": null,
            "message_id": 3, "user_id": 9,
            "message": [{"type": "text", "data": {"text": "[hi]"}}],
            "raw_message": "&#91;hi&#93;", "font": 0, "sender": {"user_id": 9},
        })
    );
}

#[test]
fn string_format_replaces_message_with_cq() {
    let value = event(EventKind::Message(group_message())).to_value(MessageFormat::String);
    assert_eq!(value["message"], "&#91;hi&#93;");
}

#[test]
fn private_message_and_extra_fields() {
    let mut message = MessageEvent::new(
        MessageScene::Private {
            sub_type: PrivateSubType::Friend,
        },
        1,
        2,
        Message::text("x"),
        Sender {
            user_id: 2,
            ..Sender::default()
        },
    );
    message
        .extra
        .insert("qq".into(), json!({"message_id": "abc"}));
    let value = event(EventKind::Message(message)).to_value(MessageFormat::Array);
    assert_eq!(value["message_type"], "private");
    assert_eq!(value["sub_type"], "friend");
    assert_eq!(value["qq"]["message_id"], "abc");
    assert!(value.get("group_id").is_none());
}

#[test]
fn standard_and_custom_notices() {
    let increase = event(EventKind::Notice(NoticeEvent::group_increase(
        GroupIncreaseType::Invite,
        7,
        9,
        0,
    )))
    .to_value(MessageFormat::Array);
    assert_eq!(
        increase,
        json!({"time": 100, "self_id": 42, "post_type": "notice", "notice_type": "group_increase",
               "sub_type": "invite", "group_id": 7, "operator_id": 0, "user_id": 9})
    );
    let decrease = NoticeEvent::group_decrease(GroupDecreaseType::KickMe, 7, 9, 0);
    assert_eq!(decrease.sub_type.as_deref(), Some("kick_me"));
    let mut custom = NoticeEvent::custom("c2c_msg_reject", 9);
    custom.extra.insert("qq".into(), json!({}));
    let value = event(EventKind::Notice(custom)).to_value(MessageFormat::Array);
    assert_eq!(
        value,
        json!({"time": 100, "self_id": 42, "post_type": "notice", "notice_type": "c2c_msg_reject", "user_id": 9, "qq": {}})
    );
}

#[test]
fn requests_and_meta_events() {
    let request = RequestEvent {
        kind: RequestKind::Group {
            sub_type: GroupRequestType::Add,
            group_id: 7,
        },
        user_id: 9,
        comment: "hi".into(),
        flag: "f".into(),
        extra: Map::new(),
    };
    let value = event(EventKind::Request(request)).to_value(MessageFormat::Array);
    assert_eq!(value["post_type"], "request");
    assert_eq!(value["request_type"], "group");
    assert_eq!(value["sub_type"], "add");
    assert_eq!(value["flag"], "f");

    let lifecycle = event(EventKind::MetaEvent(MetaEvent::Lifecycle {
        sub_type: LifecycleType::Connect,
    }))
    .to_value(MessageFormat::Array);
    assert_eq!(
        lifecycle,
        json!({"time": 100, "self_id": 42, "post_type": "meta_event", "meta_event_type": "lifecycle", "sub_type": "connect"})
    );

    let heartbeat = event(EventKind::MetaEvent(MetaEvent::Heartbeat {
        status: BotStatus {
            online: None,
            good: true,
            extra: Map::new(),
        },
        interval: 15000,
    }))
    .to_value(MessageFormat::Array);
    assert_eq!(heartbeat["meta_event_type"], "heartbeat");
    assert_eq!(heartbeat["status"], json!({"online": null, "good": true}));
    assert_eq!(heartbeat["interval"], 15000);
}

#[test]
fn friend_request_has_no_sub_type() {
    let request = RequestEvent {
        kind: RequestKind::Friend,
        user_id: 9,
        comment: String::new(),
        flag: "f".into(),
        extra: Map::new(),
    };
    let value = event(EventKind::Request(request)).to_value(MessageFormat::Array);
    assert_eq!(value["request_type"], "friend");
    assert!(value.get("sub_type").is_none());
}
