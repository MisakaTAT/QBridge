use qbridge_qq::{Dispatch, Envelope};
use serde_json::json;

fn envelope(t: &str, d: serde_json::Value) -> Envelope {
    Envelope::parse(Some("EVENT".into()), t, d)
}

#[test]
fn parses_c2c_message() {
    let event = envelope(
        "C2C_MESSAGE_CREATE",
        json!({
            "id": "M1",
            "content": "hello",
            "timestamp": "2026-01-01T00:00:00Z",
            "author": {"user_openid": "U1", "username": "u"},
            "message_scene": {"source": "chat"},
            "attachments": [{
                "content_type": "image/png",
                "url": "https://example.com/a.png",
                "filename": "a.png",
                "size": 123
            }]
        }),
    );
    assert_eq!(event.id.as_deref(), Some("EVENT"));
    let message = match event.dispatch {
        Dispatch::C2cMessage(message) => message,
        other => panic!("unexpected dispatch: {other:?}"),
    };
    assert_eq!(message.id, "M1");
    assert_eq!(message.content, "hello");
    assert_eq!(message.author.user_openid(), Some("U1"));
    assert!(message.message_scene.is_some());
    assert_eq!(
        message.attachments[0].content_type.as_deref(),
        Some("image/png")
    );
    assert_eq!(message.attachments[0].size, Some(123));
}

#[test]
fn parses_group_messages_and_keeps_group_openid() {
    for t in ["GROUP_AT_MESSAGE_CREATE", "GROUP_MESSAGE_CREATE"] {
        let event = envelope(
            t,
            json!({
                "id": "M1",
                "content": "hi",
                "group_openid": "G1",
                "timestamp": "2026-01-01T00:00:00Z",
                "author": {"member_openid": "M1", "member_role": "member"}
            }),
        );
        let group = match event.dispatch {
            Dispatch::GroupAtMessage(message) => message,
            Dispatch::GroupMessage(message) => message,
            other => panic!("unexpected dispatch: {other:?}"),
        };
        assert_eq!(group.group_openid, "G1");
        assert_eq!(group.author.member_openid(), Some("M1"));
        assert_eq!(group.author.member_role.as_deref(), Some("member"));
    }
}

#[test]
fn parses_notice_events() {
    type Recognizer = fn(&Dispatch) -> bool;
    let cases: &[(&str, Recognizer)] = &[
        ("FRIEND_ADD", |d| matches!(d, Dispatch::FriendAdd(_))),
        ("FRIEND_DEL", |d| matches!(d, Dispatch::FriendDel(_))),
        ("GROUP_ADD_ROBOT", |d| {
            matches!(d, Dispatch::GroupAddRobot(_))
        }),
        ("GROUP_DEL_ROBOT", |d| {
            matches!(d, Dispatch::GroupDelRobot(_))
        }),
        ("GROUP_MSG_RECEIVE", |d| {
            matches!(d, Dispatch::GroupMsgReceive(_))
        }),
        ("GROUP_MSG_REJECT", |d| {
            matches!(d, Dispatch::GroupMsgReject(_))
        }),
        ("C2C_MSG_RECEIVE", |d| {
            matches!(d, Dispatch::C2cMsgReceive(_))
        }),
        ("C2C_MSG_REJECT", |d| matches!(d, Dispatch::C2cMsgReject(_))),
    ];
    for (t, recognizes) in cases {
        let data = if t.starts_with("GROUP") {
            json!({"group_openid": "G1", "op_member_openid": "M1", "timestamp": "2026-01-01T00:00:00Z"})
        } else {
            json!({"openid": "U1", "timestamp": "2026-01-01T00:00:00Z"})
        };
        let event = envelope(t, data);
        assert!(
            recognizes(&event.dispatch),
            "failed to parse {t}: {:?}",
            event.dispatch
        );
    }
}

#[test]
fn parses_interaction() {
    let event = envelope(
        "INTERACTION_CREATE",
        json!({
            "id": "I1",
            "type": 11,
            "chat_type": 1,
            "group_openid": "G1",
            "group_member_openid": "M1",
            "button_data": "b",
            "button_id": "id",
            "timestamp": "2026-01-01T00:00:00Z"
        }),
    );
    let interaction = match event.dispatch {
        Dispatch::Interaction(interaction) => interaction,
        other => panic!("unexpected dispatch: {other:?}"),
    };
    assert_eq!(interaction.id, "I1");
    assert_eq!(interaction.kind, Some(11));
    assert_eq!(interaction.chat_type, Some(1));
    assert_eq!(interaction.group_openid.as_deref(), Some("G1"));
}

#[test]
fn downgrades_unknown_events_and_bad_payloads() {
    let unknown = envelope("SOMETHING_NEW", json!({"x": 1}));
    assert!(matches!(
        unknown.dispatch,
        Dispatch::Unknown { ref t, .. } if t == "SOMETHING_NEW"
    ));

    let ready = envelope(
        "READY",
        json!({"session_id": "S", "user": {"username": "bot"}}),
    );
    assert!(matches!(ready.dispatch, Dispatch::Unknown { ref t, .. } if t == "READY"));

    let broken = envelope("C2C_MESSAGE_CREATE", json!({"content": "missing id"}));
    assert!(matches!(
        broken.dispatch,
        Dispatch::Unknown { ref t, .. } if t == "C2C_MESSAGE_CREATE"
    ));
}

#[test]
fn envelope_from_json_requires_a_type() {
    let event = Envelope::from_json(&json!({
        "id": "E1",
        "t": "FRIEND_ADD",
        "d": {"openid": "U1"}
    }))
    .unwrap();
    assert_eq!(event.id.as_deref(), Some("E1"));
    assert!(matches!(event.dispatch, Dispatch::FriendAdd(_)));
    assert!(Envelope::from_json(&json!({"id": "E1", "d": {}})).is_none());
}

#[test]
fn parses_group_member_and_join_request_events() {
    let add = envelope(
        "GROUP_MEMBER_ADD",
        json!({
            "group_openid": "G1",
            "member_openid": "M1",
            "op_member_openid": "M2",
            "timestamp": "2026-01-01T00:00:00Z"
        }),
    );
    assert!(matches!(add.dispatch, Dispatch::GroupMemberAdd(_)));
    let remove = envelope(
        "GROUP_MEMBER_REMOVE",
        json!({
            "group_openid": "G1",
            "member_openid": "M1",
            "timestamp": 1795140226
        }),
    );
    assert!(matches!(remove.dispatch, Dispatch::GroupMemberRemove(_)));
    let join = envelope(
        "GROUP_JOIN_REQUEST",
        json!({
            "group_openid": "G1",
            "join_request_id": "J1",
            "member_openid": "M1",
            "apply_at": "2026-01-01T00:00:00Z"
        }),
    );
    let Dispatch::GroupJoinRequest(request) = join.dispatch else {
        panic!("unexpected dispatch")
    };
    assert_eq!(request.join_request_id, "J1");
    assert_eq!(request.member_openid, "M1");
}
